# AWS IoT Rust Examples

This repository contains examples demonstrating how to use the AWS IoT SDK for Rust. These examples are also integrated into our CI pipeline as integration tests.

## Examples

### AWS IoT Fleet Provisioning (`provisioning.rs`)

This example demonstrates how to use the AWS IoT Fleet Provisioning service to provision a device.

**Requirements:**

* An AWS account with AWS IoT Core and AWS IoT Fleet Provisioning configured.
* A device certificate and private key. You can generate these using OpenSSL or your preferred method.
* A provisioning template configured in your AWS account.

**To run the example:**

1. **Create a PKCS #12 (.pfx) identity file:**
   If you haven't already, create a PKCS #12 (.pfx) file containing your device certificate and private key.  You can use OpenSSL for this:

   ```bash
   openssl pkcs12 -export -out claim_identity.pfx -inkey private.pem.key -in certificate.pem.crt -certfile root-ca.pem
   ```
   Replace `private.pem.key`, `certificate.pem.crt`, and `root-ca.pem` with your actual file names.

2. **Store the Identity File:**
    Place the `claim_identity.pfx` file in the `tests/secrets/` directory.

3. **Set Environment Variables:**
    Set the following environment variables:
   * `IDENTITY_PASSWORD`: The password you set for the `claim_identity.pfx` file.
   * `AWS_HOSTNAME`: Your AWS IoT endpoint. You can find this in the AWS IoT console.

4. **Run the Test:**

   ```bash
   cargo test --test provisioning --features "log,std"
   ```

### AWS IoT OTA (`ota_mqtt.rs`)

This example demonstrates how to perform an over-the-air (OTA) firmware update using AWS IoT Jobs.

**Requirements:**

* An AWS account with AWS IoT Core and AWS IoT Jobs configured.
* A device certificate and private key.
* A PKCS #12 (.pfx) file containing the device certificate and private key (see previous example for creation instructions).
* An OTA update job created in your AWS account.

**To run the example:**

1. **Create an OTA Job:** Create an OTA update job.  You can find instructions on how to do this in the AWS IoT documentation or refer to the `scripts/create_ota.sh` script for inspiration.
2. **Store the Identity File:** Ensure the `identity.pfx` file (containing your device certificate and private key) is located in the `tests/secrets/` directory.
3. **Set Environment Variables:**
   * `IDENTITY_PASSWORD`: The password for your `identity.pfx` file.
   * `AWS_HOSTNAME`: Your AWS IoT endpoint.

4. **Run the Test:**

   ```bash
   cargo test --test ota_mqtt --features "log,std"
   ```

### AWS IoT Shadows (`shadows.rs`)

This example demonstrates how to interact with AWS IoT device shadows. Device shadows allow you to store and retrieve the latest state of your devices even when they are offline.

**Requirements:**

* An AWS account with AWS IoT Core and AWS IoT Device Shadows configured.
* A device certificate and private key.
* A PKCS #12 (.pfx) file containing the device certificate and private key (see previous examples for creation instructions).

**To run the example:**

1. **Store the Identity File:** Ensure the `claim_identity.pfx` file (containing your device certificate and private key) is in the `tests/secrets/` directory.
2. **Set Environment Variables:**
   * `IDENTITY_PASSWORD`: The password for your `claim_identity.pfx` file.
   * `AWS_HOSTNAME`: Your AWS IoT endpoint.

3. **Run the Test:**

   ```bash
   cargo test --test shadows --features "log,std"
   ```
