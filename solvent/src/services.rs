//! Runtime-managed services and shared service snapshots.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

pub trait Service: Send {
    fn tick(&mut self, now: u64);
}

pub(crate) static SERVICES: Mutex<Vec<Box<dyn Service>>> = Mutex::new(Vec::new());

pub fn register_service(service: Box<dyn Service>) {
    SERVICES.lock().push(service);
}

#[cfg(not(nitrogen_no_iwlwifi))]
pub use crate::network_manager::register_wifi_service;

#[allow(dead_code)]
pub enum WifiAction {
    Connect(bonder::wifi::Ssid, Option<String>),
}

pub static WIFI_ACTION_QUEUE: Mutex<Vec<WifiAction>> = Mutex::new(Vec::new());

pub struct NetworkSnapshot {
    pub aps: Vec<lattice::network_menu::ApDisplay>,
    pub status: lattice::network_menu::NetStatus,
}

pub static NETWORK_SNAPSHOT: Mutex<NetworkSnapshot> = Mutex::new(NetworkSnapshot {
    aps: Vec::new(),
    status: lattice::network_menu::NetStatus::NoDevice,
});
