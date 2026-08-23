//! Minimal JSON-line client used by the CLI to talk to a manager daemon.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::paths::Paths;

/// Send one request over the manager's unix socket and return the response
/// `data` (or an error message). Thin wrapper over [`crate::platform::net::request`].
pub fn request_json(socket: &std::path::Path, req: &Value) -> Result<Value, String> {
    crate::platform::net::request(socket, req)
}

pub struct Client {
    pub user: bool,
    pub socket: PathBuf,
}

impl Client {
    pub fn for_mode(user: bool) -> Result<Client, String> {
        let paths = if user {
            Paths::user()?
        } else {
            Paths::system()
        };
        Ok(Client {
            user,
            socket: paths.control_socket(),
        })
    }

    /// Send one request and return the response object.
    pub fn request(&self, req: &Value) -> Result<Value, String> {
        request_json(&self.socket, req)
    }

    /// Convenience: send a unit op and return its data.
    pub fn units_op(&self, op: &str, units: &[String]) -> Result<Value, String> {
        self.request(&json!({"op": op, "units": units}))
    }

    /// Convenience: send a simple op with no unit args.
    pub fn simple_op(&self, op: &str) -> Result<Value, String> {
        self.request(&json!({"op": op}))
    }

    pub fn op_with(&self, op: &str, extra: Value) -> Result<Value, String> {
        let mut m = serde_json::Map::new();
        m.insert("op".into(), json!(op));
        if let Some(extra) = extra.as_object() {
            for (k, v) in extra {
                m.insert(k.clone(), v.clone());
            }
        }
        self.request(&Value::Object(m))
    }
}
