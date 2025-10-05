#!/bin/bash

rm -f ./mindshard.cer ./mindshardkey

openssl genrsa -out mindshard.key 4096

openssl req \
    -x509 \
    -new \
    -nodes \
    -key mindshard.key \
    -sha512 \
    -days 3650 \
    -out mindshard.cer \
    -subj "/CN=mindshard"
