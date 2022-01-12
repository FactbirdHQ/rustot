# AWS IoT Fleet Provisioner

By using AWS IoT fleet provisioning, AWS IoT can generate and securely deliver device certificates and
private keys to your devices when they connect to AWS IoT for the first time. AWS IoT provides client
certificates that are signed by the Amazon Root certificate authority (CA).
There are two ways to use fleet provisioning:
- By claim (implemented by this crate)
- By trusted user (**not** implemented by this crate)

## Provisioning by claim

Devices can be manufactured with a provisioning claim certificate and private key (which are special
purpose credentials) embedded in them. If these certificates are registered with AWS IoT, the service can
exchange them for unique device certificates that the device can use for regular operations. This process
includes the following steps:

**Before you deliver the device**
1. Call `CreateProvisioningTemplate` to create a provisioning template. This API
   returns a template ARN. For more information, see Device provisioning MQTT
   API documentation. 
   
   You can also create a fleet provisioning template in the AWS IoT console. 
   - a. From the navigation pane, choose Onboard, then choose
   Fleet provisioning templates. 
   - b. Choose Create and follow the prompts.
2. Create certificates and associated private keys to be used as provisioning claim certificates.
3. Register these certificates with AWS IoT and associate an IoT policy that restricts the use of the
certificates. The following example IoT policy restricts the use of the certificate associated with this
policy to provisioning devices.
4. Give the AWS IoT service permission to create or update IoT resources such as things and certificates
in your account when provisioning devices. Do this by attaching the AWSIoTThingsRegistration
managed policy to an IAM role (called the provisioning role) that trusts the AWS IoT service principal.
5. Manufacture the device with the provisioning claim certificate securely
   embedded in it.

<hr>

## Example

You can find an example of how to use this crate for provisioning in the `examples/provisioning.rs` file. This example can be run by `RUST_LOG=trace AWS_HOSTNAME=xxxxxxxx-ats.iot.eu-west-1.amazonaws.com cargo r --example provisioning --features log`, assuming you have an `examples/secrets/claim_identity.pfx` file with the claiming credentials. 

pfx identity files can be created from a set of device certificate and private key using OpenSSL as: `openssl pkcs12 -export -out claim_identity.pfx -inkey private.pem.key -in certificate.pem.crt -certfile root-ca.pem`