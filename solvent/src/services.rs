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

#[cfg(all(target_arch = "x86_64", not(nitrogen_no_iwlwifi)))]
pub use crate::network_manager::register_wifi_service;
/// Wi-Fi action queue: x86-only until the ANE Wi-Fi bring-up lands.
#[cfg(target_arch = "x86_64")]
#[allow(dead_code)]
pub enum WifiAction {
    Connect(bonder::wifi::Ssid, Option<String>),
}

#[cfg(target_arch = "x86_64")]
pub static WIFI_ACTION_QUEUE: Mutex<Vec<WifiAction>> = Mutex::new(Vec::new());
