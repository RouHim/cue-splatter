FROM alpine:3

RUN apk update && apk add --no-cache ffmpeg

WORKDIR /app

# Add user and group 1000
RUN addgroup -g 1000 -S app && adduser -u 1000 -S app -G app
USER app

COPY target/x86_64-unknown-linux-musl/release/cue-splatter /bin/cue-splatter

ENTRYPOINT ["/bin/sh"]
CMD ["-l"]