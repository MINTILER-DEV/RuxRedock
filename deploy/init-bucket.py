"""Idempotent bucket provisioning using credentials supplied through environment."""
import os
import time
import boto3
from botocore.exceptions import ClientError, BotoCoreError

client = boto3.client("s3", endpoint_url=os.environ["S3_ENDPOINT"], region_name="us-east-1")
bucket = os.environ.get("S3_BUCKET", "ruxredock")
for attempt in range(90):
    try:
        try:
            client.head_bucket(Bucket=bucket)
        except ClientError as error:
            if error.response["ResponseMetadata"]["HTTPStatusCode"] == 404:
                client.create_bucket(Bucket=bucket)
            else:
                raise
        print("Ciphertext bucket is ready.")
        break
    except (ClientError, BotoCoreError):
        if attempt == 89:
            raise
        time.sleep(2)
