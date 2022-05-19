#!/usr/bin/env bash

# Cleans up all OTA Jobs with status QUEUED or IN_PROGRESS by canceling them.
#

THING="rustot-test"

CROSS_ACC=$(printf "AWS_ACCESS_KEY_ID=%s AWS_SECRET_ACCESS_KEY=%s AWS_SESSION_TOKEN=%s" \
    $(aws sts assume-role \
        --role-arn arn:aws:iam::411974994697:role/ExternalAccessProvisionIoT \
        --role-session-name "RustotIntegration" \
        --query "Credentials.[AccessKeyId,SecretAccessKey,SessionToken]" \
        --output text))

QUEUED_JOBS=$( (
    export $CROSS_ACC
    aws iot list-job-executions-for-thing --thing-name $THING --status QUEUED --query 'executionSummaries[].jobId' --output text
))
IN_PROGRESS_JOBS=$( (
    export $CROSS_ACC
    aws iot list-job-executions-for-thing --thing-name $THING --status IN_PROGRESS --query 'executionSummaries[].jobId' --output text
))

for JOB_ID in $QUEUED_JOBS $IN_PROGRESS_JOBS; do
    echo "Canceling job: $JOB_ID"
    (
        export $CROSS_ACC
        aws iot cancel-job --job-id $JOB_ID --force
    )
done
