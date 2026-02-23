# AWS IoT Shadows

<hr>

## Example / Test

You can find an example of how to use this crate for iot shadow states in the `tests/shadows.rs` file. This example can be run by `RUST_LOG=trace THING_NAME="MyTestThing" AWS_HOSTNAME=xxxxxxxx-ats.iot.eu-west-1.amazonaws.com cargo test --test 'shadows' --features "log"`, assuming you have an `tests/secrets/identity.pfx` file with valid AWS IoT credentials. If your `identity.pfx` is password protected, it can be supplied by setting the `IDENTITY_PASSWORD` env variable.

pfx identity files can be created from a set of device certificate and private key using OpenSSL as: `openssl pkcs12 -export -out identity.pfx -inkey private.pem.key -in certificate.pem.crt -certfile root-ca.pem`

The example functions as a CI integration test, that is run against Factbirds integration account on every PR. This test will run through a statemachine of shadow delete, updates and gets from both device & cloud side with assertions in between.
