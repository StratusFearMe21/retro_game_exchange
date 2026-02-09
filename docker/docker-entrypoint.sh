#!/bin/sh

SERVICE_ID=$(dig -x "$(hostname -i)" +short) \
  exec "$@"
