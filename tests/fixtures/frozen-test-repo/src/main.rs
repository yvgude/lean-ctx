mod utils;

use std::collections::HashMap;

/// Application entry point.
fn main() {
    let config = load_config();
    let server = Server::new(config.port);
    server.start();
    println!("Server running on port {}", config.port);
}

struct Config {
    port: u16,
    host: String,
    debug: bool,
}

impl Config {
    fn default() -> Self {
        Self {
            port: 8080,
            host: String::from("0.0.0.0"),
            debug: false,
        }
    }
}

fn load_config() -> Config {
    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    Config { port, ..Config::default() }
}

struct Server {
    port: u16,
}

impl Server {
    fn new(port: u16) -> Self {
        Self { port }
    }

    fn start(&self) {
        println!("Listening on :{}", self.port);
        utils::log("server started");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let cfg = Config::default();
        assert_eq!(cfg.port, 8080);
    }
}
