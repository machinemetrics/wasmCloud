# NATS Capability Provider

This capability provider is an implementation of the `wasmcloud:keyvalue` contract with a NATS JetStream KV backend. It exposes the KV get, set, contains, del and increment functionality to actors. List and set functionalities are not implemented at this time.

## Link Definition Configuration Settings

To configure this provider, use the following link settings in link definitions:

| Property      | Description                                                                                            |
| :------------ | :----------------------------------------------------------------------------------------------------- |
| `URI`         | NATS connection uri's. If not specified, the default is `0.0.0.0:4222`                                 |
| `CLIENT_JWT`  | Optional JWT auth token. For JWT authentication, both `CLIENT_JWT` and `CLIENT_SEED` must be provided. |
| `CLIENT_SEED` | Private seed for JWT authentication.                                                                   |
