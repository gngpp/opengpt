# 定义参数
ARG BASE_IMAGE

FROM $BASE_IMAGE

ARG TARGETPLATFORM

WORKDIR /home/rust/src

LABEL org.opencontainers.image.authors "gngpp <gngppz@gmail.com>"
LABEL org.opencontainers.image.source https://github.com/gngpp/opengpt

RUN if [ "${TARGETPLATFORM}" = "linux/arm64" ]; then \
        apt update -y && apt install -y gcc-aarch64-linux-gnu; \
    elif [ "${TARGETPLATFORM}" = "linux/amd64" ]; then \
        apt update -y && apt install -y gcc-multilib; \
    fi