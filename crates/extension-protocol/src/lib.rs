//! Versioned, bounded line protocol for the out-of-process extension host.

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Permission {
    ReadDocument,
    EditDocument,
    WorkspaceRead,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Request {
    Ping { id: u64 },
    Execute { id: u64, module: String, fuel: u64 },
    Cancel { id: u64 },
    Shutdown,
}

impl Request {
    /// Decodes one bounded tab-separated frame.
    ///
    /// # Errors
    /// Returns an error for oversized, malformed, or unsupported frames.
    pub fn decode(frame: &str) -> Result<Self, &'static str> {
        if frame.len() > MAX_FRAME_BYTES {
            return Err("frame too large");
        }
        let fields: Vec<_> = frame.trim_end().split('\t').collect();
        match fields.as_slice() {
            ["1", "ping", id] => Ok(Self::Ping {
                id: id.parse().map_err(|_| "invalid id")?,
            }),
            ["1", "execute", id, module, fuel] => Ok(Self::Execute {
                id: id.parse().map_err(|_| "invalid id")?,
                module: (*module).to_owned(),
                fuel: fuel.parse().map_err(|_| "invalid fuel")?,
            }),
            ["1", "cancel", id] => Ok(Self::Cancel {
                id: id.parse().map_err(|_| "invalid id")?,
            }),
            ["1", "shutdown"] => Ok(Self::Shutdown),
            [version, ..] if *version != "1" => Err("unsupported protocol version"),
            _ => Err("malformed frame"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unknown_versions() {
        assert_eq!(
            Request::decode("2\tping\t1"),
            Err("unsupported protocol version")
        );
    }
    #[test]
    fn decodes_execute() {
        assert!(matches!(
            Request::decode("1\texecute\t7\tplugin.wasm\t1000"),
            Ok(Request::Execute {
                id: 7,
                fuel: 1000,
                ..
            })
        ));
    }
}
