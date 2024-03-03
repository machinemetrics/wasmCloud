use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{ debug, error, instrument };
use wascap::prelude::KeyPair;

use wasmcloud_provider_wit_bindgen::deps::{
    async_trait::async_trait,
    serde_json,
    wasmcloud_provider_sdk::core::{ HostData, LinkDefinition },
    wasmcloud_provider_sdk::Context,
};

mod connection;
use connection::ConnectionConfig;

wasmcloud_provider_wit_bindgen::generate!({
    impl_struct: NatsKeyvalueProvider,
    contract: "wasmcloud:keyvalue",
    wit_bindgen_cfg: "provider-keyvalue-nats"
});

/// Nats implementation for wasmcloud:keyvalue
#[derive(Default, Clone)]
pub struct NatsKeyvalueProvider {
    // One NATS KV Store per actor. TODO: Could refactor to a single connection maintaining multiple stores
    actors: Arc<RwLock<HashMap<String, async_nats::jetstream::kv::Store>>>,
    default_config: ConnectionConfig,
}

impl NatsKeyvalueProvider {
    /// Build a [`NatsKeyvalueProvider`] from [`HostData`]
    pub fn from_host_data(host_data: &HostData) -> NatsKeyvalueProvider {
        host_data.config_json
            .as_ref()
            .map(|c| {
                // empty string becomes the default configuration
                if c.trim().is_empty() {
                    NatsKeyvalueProvider::default()
                } else {
                    let config: ConnectionConfig = serde_json
                        ::from_str(c)
                        .expect("JSON deserialization from connection config should have worked");
                    NatsKeyvalueProvider {
                        default_config: config,
                        ..Default::default()
                    }
                }
            })
            .unwrap_or_default()
    }

    /// Generic function to get the KV Store for an actor given a context.
    async fn get_store(&self, ctx: Context) -> anyhow::Result<async_nats::jetstream::kv::Store> {
        let actor_id = ctx.actor.as_ref().expect("Failed to get actor id");
        let rd = self.actors.read().await;
        match rd.get(actor_id) {
            Some(store) => Ok(store.clone()),
            None => Err(anyhow::anyhow!("Failed to get KV Store for actor")),
        }
    }

    /// Attempt to connect to nats url (with jwt credentials, if provided)
    async fn connect(
        &self,
        cfg: ConnectionConfig,
        _ld: &LinkDefinition
    ) -> anyhow::Result<async_nats::Client> {
        let opts = match (cfg.auth_jwt, cfg.auth_seed) {
            (Some(jwt), Some(seed)) => {
                let key_pair = std::sync::Arc::new(KeyPair::from_seed(&seed)?);
                async_nats::ConnectOptions::with_jwt(jwt, move |nonce| {
                    let key_pair = key_pair.clone();
                    async move { key_pair.sign(&nonce).map_err(async_nats::AuthError::new) }
                })
            }
            (None, None) => async_nats::ConnectOptions::default(),
            _ => {
                anyhow::bail!("must provide both jwt and seed for jwt authentication");
            }
        };

        // Use the first visible cluster_uri
        let url = cfg.cluster_uris.first().unwrap();

        let client = opts
            .name("NATS Keyvalue Provider") // allow this to show up uniquely in a NATS connection list
            .connect(url).await?;

        Ok(client)
    }
}

/// Handle provider control commands
/// put_link (new actor link command), del_link (remove link command), and shutdown
#[async_trait]
impl WasmcloudCapabilityProvider for NatsKeyvalueProvider {
    /// Provider should perform any operations needed for a new link,
    /// including setting up per-actor resources, and checking authorization.
    /// If the link is allowed, return true, otherwise return false to deny the link.
    #[instrument(level = "debug", skip(self, ld), fields(actor_id = %ld.actor_id))]
    async fn put_link(&self, ld: &LinkDefinition) -> bool {
        // If the link definition values are empty, use the default connection configuration
        let config = if ld.values.is_empty() {
            self.default_config.clone()
        } else {
            // create a config from the supplied values and merge that with the existing default
            match ConnectionConfig::from_tuples(&ld.values) {
                Ok(cc) => self.default_config.merge(&cc),
                Err(e) => {
                    error!("Failed to build connection configuration: {e:?}");
                    return false;
                }
            }
        };

        let mut update_map = self.actors.write().await;
        let client = match self.connect(config, ld).await {
            Ok(b) => b,
            Err(e) => {
                error!("Failed to connect to NATS: {e:?}");
                return false;
            }
        };

        let store = async_nats::jetstream
            ::new(client)
            .create_key_value(async_nats::jetstream::kv::Config {
                bucket: ld.actor_id.to_string(),
                ..Default::default()
            }).await
            .expect("Failed to create key value");

        update_map.insert(ld.actor_id.to_string(), store);

        true
    }

    #[instrument(level = "debug", skip(self))]
    async fn delete_link(&self, actor_id: &str) {
        // Remove the actor's KV Store and close the NATS connection
        debug!("finished processing delete link for actor [{}]", actor_id);
    }

    async fn shutdown(&self) {
        // Close all NATS connections and clear the actor KV Store map
        let mut aw = self.actors.write().await;
        aw.clear();
    }
}

#[async_trait]
#[allow(unused_variables)]
impl WasmcloudKeyvalueKeyValue for NatsKeyvalueProvider {
    #[instrument(level = "debug", skip(self, ctx, arg), fields(actor_id = ?ctx.actor, key = %arg.key))]
    async fn set(&self, ctx: Context, arg: SetRequest) -> () {
        let store = match self.get_store(ctx).await {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to get KV Store: {e:?}");
                return;
            },
        };

        store.put(arg.key, arg.value.into()).await.expect("Failed to put key value");
    }

    #[instrument(level = "debug", skip(self, ctx, arg), fields(actor_id = ?ctx.actor, key = %arg.to_string()))]
    async fn get(&self, ctx: Context, arg: String) -> GetResponse {
        let store = match self.get_store(ctx).await {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to get KV Store: {e:?}");
                return GetResponse {
                    value: String::new(),
                    exists: false,
                };
            },
        };

        match store.get(arg).await {
            Ok(Some(v)) =>
                GetResponse {
                    value: String::from_utf8_lossy(&v).into_owned(),
                    exists: true,
                },
            _ =>
                GetResponse {
                    value: String::new(),
                    exists: false,
                },
        }
    }

    #[instrument(level = "debug", skip(self, ctx, arg), fields(actor_id = ?ctx.actor, key = %arg.to_string()))]
    async fn contains(&self, ctx: Context, arg: String) -> bool {
        let store = match self.get_store(ctx).await {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to get KV Store: {e:?}");
                return false;
            },
        };

        match store.get(arg).await {
            Ok(Some(_)) => true,
            _ => false,
        }
    }

    #[instrument(level = "debug", skip(self, ctx, arg), fields(actor_id = ?ctx.actor, key = %arg.to_string()))]
    async fn del(&self, ctx: Context, arg: String) -> bool {
        let store = match self.get_store(ctx).await {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to get KV Store: {e:?}");
                return false;
            },
        };

        // Check if the key exists as NATS KV Store does not return an error if the key does not exist
        let exists = match store.get(&arg).await {
            Ok(Some(_)) => true,
            _ => false,
        };
        if !exists {
            return false;
        }

        // Delete the key, return true if the key was deleted, false if error
        match store.delete(&arg).await {
            Ok(_) => true,
            _ => false,
        }
    }

    #[instrument(level = "debug", skip(self, ctx, arg), fields(actor_id = ?ctx.actor, key = %arg.key))]
    async fn increment(&self, ctx: Context, arg: IncrementRequest) -> f32 {
        let store = match self.get_store(ctx).await {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to get KV Store: {e:?}");
                return 0.0;
            },
        };

        // Get the current value of the key, if it does not exist, default to 0.0
        let value = match store.get(&arg.key).await {
            Ok(Some(v)) => {
                let v = String::from_utf8_lossy(&v).into_owned();
                v.parse::<f32>().unwrap_or(0.0)
            },
            _ => 0.0,
        };

        let new_value = value + arg.value;
        match store.put(arg.key, new_value.to_string().into()).await {
            Ok(_) => new_value,
            _ => {
                error!("Failed to increment key");
                0.0
            },
        }
    }

    // The following methods are not implemented as they are not relevant to the NATS KV Store
    async fn list_add(&self, ctx: Context, arg: ListAddRequest) -> u32 {
        error!("`list_add` not implemented");
        0
    }

    async fn list_clear(&self, ctx: Context, arg: String) -> bool {
        error!("`list_clear` not implemented");
        false
    }

    async fn list_del(&self, ctx: Context, arg: ListDelRequest) -> bool {
        error!("`list_del` not implemented");
        false
    }

    async fn list_range(&self, ctx: Context, arg: ListRangeRequest) -> Vec<String> {
        error!("`list_range` not implemented");
        Vec::new()
    }

    async fn set_add(&self, ctx: Context, arg: SetAddRequest) -> u32 {
        error!("`set_add` not implemented");
        0
    }

    async fn set_clear(&self, ctx: Context, arg: String) -> bool {
        error!("`set_clear` not implemented");
        false
    }

    async fn set_del(&self, ctx: Context, arg: SetDelRequest) -> u32 {
        error!("`set_del` not implemented");
        0
    }

    async fn set_intersection(&self, ctx: Context, arg: Vec<String>) -> Vec<String> {
        error!("`set_intersection` not implemented");
        Vec::new()
    }

    async fn set_query(&self, ctx: Context, arg: String) -> Vec<String> {
        error!("`set_query` not implemented");
        Vec::new()
    }

    async fn set_union(&self, ctx: Context, arg: Vec<String>) -> Vec<String> {
        error!("`set_union` not implemented");
        Vec::new()
    }
}

#[cfg(test)]
mod test {
    use crate::{ serde_json, ConnectionConfig, NatsKeyvalueProvider };
    use wasmcloud_provider_wit_bindgen::deps::wasmcloud_provider_sdk::core::LinkDefinition;
    use wasmcloud_provider_wit_bindgen::deps::wasmcloud_provider_sdk::ProviderHandler;

    #[test]
    fn test_default_connection_serialize() {
        // test to verify that we can default a config with partial input
        let input =
            r#"
{
    "cluster_uris": ["nats://soyvuh"],
    "auth_jwt": "authy",
    "auth_seed": "seedy"
}
"#;

        let config: ConnectionConfig = serde_json::from_str(input).unwrap();
        assert_eq!(config.auth_jwt.unwrap(), "authy");
        assert_eq!(config.auth_seed.unwrap(), "seedy");
        assert_eq!(config.cluster_uris, ["nats://soyvuh"]);
        assert!(config.ping_interval_sec.is_none());
    }

    #[test]
    fn test_connectionconfig_merge() {
        // second > original, individual vec fields are replace not extend
        let cc1 = ConnectionConfig {
            cluster_uris: vec!["old_server".to_string()],
            ..Default::default()
        };
        let cc2 = ConnectionConfig {
            cluster_uris: vec!["server1".to_string(), "server2".to_string()],
            auth_jwt: Some("jawty".to_string()),
            ..Default::default()
        };
        let cc3 = cc1.merge(&cc2);
        assert_eq!(cc3.cluster_uris, cc2.cluster_uris);
        assert_eq!(cc3.auth_jwt, Some("jawty".to_string()))
    }

    #[tokio::test]
    async fn test_put_link() -> anyhow::Result<()> {
        let prov = NatsKeyvalueProvider::default();
        let link_definition = LinkDefinition {
            actor_id: String::from("test_actor_id"),
            link_name: String::from("test_link_name"),
            contract_id: String::from("test_contract_id"),
            values: vec![(String::from("URI"), String::from("0.0.0.0:4222"))],
            ..Default::default()
        };

        assert!(prov.put_link(&link_definition).await, "put_link succeeded");

        let _ = prov.shutdown().await;
        Ok(())
    }
}
