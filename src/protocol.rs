use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    Status,
    Copy { data: Option<Vec<u8>> },
    Paste,
    ListDevices,
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: u64,
    pub device_id: String,
    pub device_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    Ok,
    Status {
        connected: bool,
        username: Option<String>,
        user_id: Option<u64>,
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
    Error {
        message: String,
    },
}
