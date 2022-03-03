#!/usr/bin/env bash

# Create a new OTA Job.
#

AWS_ACC=$(aws sts get-caller-identity --query "Account" --output text)

THING="rustot-test"
OTA_FILE="ota_file"
S3_BUCKET="$AWS_ACC-eu-west-1-build-artifacts"

TARGET_AWS_ACC="411974994697"

ASSETS_DIR=$(dirname $0)/../tests/assets

UPDATE_ID=$(uuidgen)
FILE_NAME=$(basename $ASSET_DIR/$OTA_FILE)
S3_KEY="factbird-duo/test/rustot/ota/$FILE_NAME-$UPDATE_ID"

aws s3 cp $ASSETS_DIR/$OTA_FILE s3://$S3_BUCKET/$S3_KEY

CROSS_ACC=$(printf "AWS_ACCESS_KEY_ID=%s AWS_SECRET_ACCESS_KEY=%s AWS_SESSION_TOKEN=%s" \
  $(aws sts assume-role \
    --role-arn arn:aws:iam::$TARGET_AWS_ACC:role/ExternalAccessProvisionIoT \
    --role-session-name "RustotIntegration" \
    --query "Credentials.[AccessKeyId,SecretAccessKey,SessionToken]" \
    --output text))

SIGNATURE=$(echo "This is my custom signature" | base64 -w0 -)

(
  export $CROSS_ACC
  aws iot create-ota-update \
    --ota-update-id $UPDATE_ID \
    --description "RustOT OTA integration test" \
    --targets "arn:aws:iot:eu-west-1:$TARGET_AWS_ACC:thing/$THING" \
    --protocols "MQTT" \
    --target-selection "SNAPSHOT" \
    --role-arn "arn:aws:iam::$TARGET_AWS_ACC:role/IoTOTAUpdateRole" \
    --files "$(
      cat <<JSON
[
  {
    "codeSigning": {
      "customCodeSigning": {
        "signature": {
          "inlineDocument": "${SIGNATURE}"
        },
        "certificateChain": {
          "certificateName": "cert",
          "inlineDocument": "signCert"
        },
        "hashAlgorithm": "sha256",
        "signatureAlgorithm": "ecdsa"
      }
    },
    "fileLocation": {
      "s3Location": {
        "bucket": "${S3_BUCKET}",
        "key": "${S3_KEY}"
      }
    },
    "fileName": "${FILE_NAME}_OTA",
    "fileType": 0
  }
]
JSON
    )"
)
