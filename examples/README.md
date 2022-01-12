This folder contains a number of examples that shows how to use this crate.

<br>

### AWS IoT Fleet Provisioning (`provisioning.rs`)
<br>

This example can be run by `RUST_LOG=trace AWS_HOSTNAME=xxxxxxxx-ats.iot.eu-west-1.amazonaws.com cargo r --example provisioning --features log`, assuming you have an `examples/secrets/claim_identity.pfx` file with the claiming credentials. 

pfx identity files can be created from a set of device certificate and private key using OpenSSL as: `openssl pkcs12 -export -out claim_identity.pfx -inkey private.pem.key -in certificate.pem.crt -certfile root-ca.pem`
<br>
<br>

### AWS IoT OTA (`ota.rs`)