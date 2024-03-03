//! NATS implementation for wasmcloud:keyvalue.

use wasmcloud_provider_wit_bindgen::deps::wasmcloud_provider_sdk;

use wasmcloud_provider_keyvalue_nats::NatsKeyvalueProvider;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    wasmcloud_provider_sdk::start_provider(
        NatsKeyvalueProvider::from_host_data(wasmcloud_provider_sdk::load_host_data()?),
        Some("provider-keyvalue-nats".to_string())
    )?;

    eprintln!("NATS keyvalue provider exiting");
    Ok(())
}
