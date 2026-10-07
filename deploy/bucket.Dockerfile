FROM python:3.12-slim
RUN pip install --no-cache-dir 'boto3>=1.38,<2'
COPY deploy/init-bucket.py /init-bucket.py
CMD ["python", "/init-bucket.py"]
