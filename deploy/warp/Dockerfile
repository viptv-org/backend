FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends tinyproxy ca-certificates && rm -rf /var/lib/apt/lists/*
COPY tinyproxy.conf /etc/tinyproxy/viptv.conf
CMD ["tinyproxy","-d","-c","/etc/tinyproxy/viptv.conf"]
