#!/usr/bin/env bash

# Registers the device in Factbird's DynamoDB containing whitelisted devices to
# be provisioned.
#
# This script will populate `tests/secrets` with `claim_certificate.pem.crt` &
# `claim_private.pem.key`, as well as combine them into `claim_identity.pfx`,
# which is password protected with `env:IDENTITY_PASSWORD`

if [[ -z "${IDENTITY_PASSWORD}" ]]; then
    echo "IDENTITY_PASSWORD environment variable is required!"
    exit 1
fi

SECRETS_DIR=$(dirname $0)/../tests/secrets

PUBLIC_KEY=$(openssl ec -in $SECRETS_DIR/sign_public.pem -pubin -text -noout 2>/dev/null | awk 'NR>2 && NR<8 {gsub(/ /,""); gsub(/:/, ""); print $0}' | tr -d '\n' | awk '{ gsub(/\r/, ""); print substr($0,3)}' | xxd -r -p | base64 -w0 -)

API_KEY_ID="wxghvqsvp8"
API_KEY=`aws apigateway get-api-key --api-key $API_KEY_ID --include-value --query value --output text`

curl -X POST -L -H "Content-Type: application/json" -H "x-api-key: $API_KEY" -d '{"uuid": "rustot-provision", "public_key": "'$PUBLIC_KEY'", "device_type": "fbduo", "hardware_version": "test"}' register.blackbird.online -o response.json

# Store files to `$SECRETS_DIR/claim_certificate.pem.crt` & `$SECRETS_DIR/claim_private.pem.key`
jq -r '.certificateId' response.json > $SECRETS_DIR/claim_certificate.id
jq -r '.certificatePem' response.json > $SECRETS_DIR/claim_certificate.pem.crt
jq -r '.privateKey' response.json > $SECRETS_DIR/claim_private.pem.key
rm response.json

openssl pkcs12 -export -out $SECRETS_DIR/claim_identity.pfx -inkey $SECRETS_DIR/claim_private.pem.key -in $SECRETS_DIR/claim_certificate.pem.crt -certfile $SECRETS_DIR/root-ca.pem -passout pass:$IDENTITY_PASSWORD
rm $SECRETS_DIR/claim_certificate.pem.crt
rm $SECRETS_DIR/claim_private.pem.key
