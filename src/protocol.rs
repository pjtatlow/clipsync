use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    Status,
    Copy { data: Option<Vec<u8>> },
    Paste,
    Send { recipient: String },
    ListDevices,
    GetPublicKey { identity: Option<String> },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: u64,
    pub device_id: String,
    pub device_name: String,
    pub owner: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    Ok,
    Status {
        connected: bool,
        identity: Option<String>,
        device_id: String,
        watching: bool,
    },
    ClipData {
        content_type: String,
        data: Vec<u8>,
    },
    Devices {
        devices: Vec<DeviceInfo>,
    },
    PublicKey {
        key: String,
    },
    Error {
        message: String,
    },
}
