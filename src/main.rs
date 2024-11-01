use std::{collections::HashMap, env, sync::Arc};
use reqwest::Response;
use reqwest_websocket::{RequestBuilderExt, Message};
use futures_util::{StreamExt, TryStreamExt, SinkExt};
use serde::Deserialize;
use rust_socketio::{ClientBuilder, Payload, RawClient};


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let mut game = Game::new();
    game.login(&args[1], &args[2]).await?;
    game.get_characters_and_servers().await.unwrap();
    Ok(())
}

#[derive(Debug, Clone)]
struct Cookie {
    name: String,
    value: String,
}

#[derive(Debug)]
struct Game {
    client: reqwest::Client,
    auth_cookie: Option<Cookie>,
    character_instances: HashMap<String, CharacterInstance>,
    character_list: HashMap<String,Character>,
    server_list: HashMap<String,Server>,
}

#[derive(Debug)]
struct CharacterInstance {
    character: Character,
    server: Server,
    websocket: reqwest_websocket::WebSocket,
    auth_cookie: Cookie,
    authed: bool,
}

impl CharacterInstance {
    async fn emit(&mut self, event: &str, data: &str) -> Result<(), Box<dyn std::error::Error>> {
        let message = Message::Text(format!("42[\"{}\",{}]", event, data));
        println!("Sent: {:?}", message);
        self.websocket.send(message).await?;
        Ok(())
    }
    async fn get_next_message(&mut self) -> Result<Message, Box<dyn std::error::Error>> {
        Ok(self.websocket.next().await.ok_or("No message received")??)
    }

    async fn pong(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.websocket.send("3".into()).await?;
        Ok(())
    }

    async fn connect(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.websocket.send("40".into()).await?;
        println!("Sent: {:?}", Message::Text("40".into()));
        Ok(())
    }
    
    async fn event_handler(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            let message = SocketIOMessage::new(self.get_next_message().await?);
            match message {
                SocketIOMessage::Neogotiation => {
                    self.connect().await?;
                },
                SocketIOMessage::Ping => {
                    self.pong().await?;
                },
                SocketIOMessage::Event { event, data } => {
                    match event.as_deref() {
                        Some("welcome") => {
                            self.emit("loaded", "{\"success\":1, \"width\":1920, \"height\":1080, \"scale\": 2 }").await?;
                        },
                        _ => {}
                    }
                    println!("Event: {:?} Data: {:?}", event, data);
                },
                SocketIOMessage::Connect { sid } => {
                    println!("Connected: {:?}", sid);
                },
                _ => {
                    println!("Unknown message: {:?}", message);
                }
            }
        } 
    }
}

#[derive(Debug)]
enum SocketIOMessage {
    Neogotiation,
    Connect { sid: Option<String> },
    Ping,
    Pong,
    Event {event: Option<String>, data: Option<serde_json::Value>},
    Unknown { code: u8, text: String },
}

impl SocketIOMessage {
    fn new(message: Message) -> SocketIOMessage {
        let text = match message {
            Message::Text(text) => text,
            _ => panic!("Not a text message this isnt socket.io at all")
        };
        let mut code = String::new();
        for char in text.chars() {
            if char.is_digit(10) {
                code.push(char);
            } else {
                break;
            }
        }
        let text = text[code.len()..].to_string();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap(); 
        let code = code.parse::<u8>().unwrap();
        match code {
            0 => SocketIOMessage::Neogotiation,
            2 => SocketIOMessage::Ping,
            3 => SocketIOMessage::Pong,
            40 => {
                let sid = json[0].as_str().map(|s| s.to_string());
                SocketIOMessage::Connect { sid }
            },
            42 => {
                let mut event = None;
                let mut data = None;
                if let serde_json::Value::String(event_string) = &json[0] {
                    event = Some(event_string.to_string());
                } 
                match &json[1] {
                    serde_json::Value::Object(_) => {
                        data = Some(json[1].clone());
                    },
                    _ => {}
                }
                               
                SocketIOMessage::Event { event, data }
            },
            _ => SocketIOMessage::Unknown { code, text }
        }
    }
}


#[derive(Deserialize, Debug, Clone)]
struct Server {
    name: String,
    region: String,
    players: u8,
    key: String,
    addr: String,
    port: u16,
}

#[derive(Deserialize, Debug, Clone)]
struct Character {
    id: String,
    name: String,
    level: u8,
    #[serde(rename = "type")]
    class: String,
    home: String,
}

impl Game {
    fn new() -> Game {
        let jar = reqwest::cookie::Jar::default();
        let client = reqwest::ClientBuilder::new()
            .cookie_store(true)
            .cookie_provider(Arc::new(jar))
            .build().unwrap(); //unwrapping is fine here, if it breaks something is very wrong.
        Game { client, auth_cookie: None, character_instances: HashMap::new(), character_list:HashMap::new(), server_list:  HashMap::new() }
    }
    
    async fn login(&mut self, username: &str, password: &str) -> Result<(), reqwest::Error> {
        let jar = reqwest::cookie::Jar::default();
        let client = reqwest::ClientBuilder::new()
            .cookie_store(true)
            .cookie_provider(Arc::new(jar))
            .build().unwrap(); //unwrapping is fine here, if it breaks something is very wrong.

        let arguments = format!("{{\"email\":\"{}\",\"password\":\"{}\",\"only_login\":true}}", username, password);
        let res = self.http_api("signup_or_login", &arguments).await?;
        let auth_cookie = res.cookies().find(|c| c.name() == "auth").unwrap();
        self.auth_cookie = Some(Cookie { name: auth_cookie.name().to_string(), value: auth_cookie.value().to_string() });
        Ok(())
    }

    async fn http_api(&self, method: &str, arguments: &str) -> Result<Response, reqwest::Error> {
        let form: reqwest::multipart::Form = reqwest::multipart::Form::new()
            .text("method", method.to_string())
            .text("arguments", arguments.to_string());
        self.client
            .post("https://adventure.land/api")
            .multipart(form)
            .send()
            .await
    }

    async fn get_characters_and_servers(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        #[derive(Deserialize, Debug)]
        struct ServersAndCharacters {
            servers: Vec<Server>,
            characters: Vec<Character>
        }
        let arguments = "{}";
        let res = self.http_api("servers_and_characters", arguments).await?;
        let bytes = res.bytes().await?;
        let mut deserialised: Vec<ServersAndCharacters> = serde_json::from_slice(&bytes)?;
        let servers_and_characters = deserialised.pop().unwrap();
        let mut server_list = HashMap::new();
        for server in servers_and_characters.servers {
            server_list.insert(server.key.clone(), server);
        }
        let mut character_list = HashMap::new();
        for character in servers_and_characters.characters {
            character_list.insert(character.name.clone(), character);
        }
        self.server_list = server_list;
        self.character_list = character_list;
        Ok(())
    }

    async fn create_character_instance(&mut self, character: Character, server: Server) -> Result<(), Box<dyn std::error::Error>> {
        let url = format!("wss://{}:{}/socket.io/?EIO=4&transport=websocket", server.addr, server.port);
        let client = reqwest::Client::new();
        let websocket = client.get(url)
            .upgrade()
            .send()
            .await?
            .into_websocket()
            .await?;
        let character_name = character.name.clone();
        let instance = CharacterInstance { character, server, websocket, auth_cookie: self.auth_cookie.clone().expect("You didnt log in before creating character instance"), authed: false };
        self.character_instances.insert(character_name, instance);
        Ok(())
    }
}
