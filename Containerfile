FROM alpine:3

RUN apk update && apk add --no-cache ffmpeg

WORKDIR /app
ENV PATH="/app:${PATH}"
RUN addgroup -g 1000 -S app && adduser -u 1000 -S app -G app
RUN chown app:app /app && chmod 755 /app

USER app

COPY target/x86_64-unknown-linux-musl/release/cue-splatter /app/cue-splatter
WORKDIR /app

ENTRYPOINT ["tail", "-f", "/dev/null"]