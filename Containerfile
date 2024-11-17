FROM alpine:20240807
ENV EDITOR=micro
RUN apk update && apk add --no-cache ffmpeg nano micro wget

WORKDIR /app
ENV PATH="/app:${PATH}"
RUN addgroup -g 1000 -S app && adduser -u 1000 -S app -G app
RUN chown app:app /app && chmod 755 /app

USER app
RUN wget -qO- https://api.github.com/repos/RouHim/cue-splatter/releases/latest | \
    grep "browser_download_url" | \
    cut -d : -f 2,3 | tr -d \" | \
    grep x86_64-unknown-linux-musl | \
    xargs wget -O /app/cue-splatter

WORKDIR /app

ENTRYPOINT ["tail", "-f", "/dev/null"]