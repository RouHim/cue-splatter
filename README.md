<p align="center">
  <img src="https://raw.githubusercontent.com/RouHim/cue-splatter/main/.github/readme/logo.png">
</p>

<p align="center">
    <i>Splits a cue file into multiple audio files.</i>
</p>

# Motivation

I recently found myself in a situation where I had a single audio file and a cue file that described the audio file. I
wanted to split the audio file into multiple audio files based on the cue file. I found a few tools that could do this,
but they were either not free, buggy, slow or not available on my platform. So I decided to write my own tool that could
do this.

# How it works

Reads the cue file and splits the referenced audio file into multiple audio files based on the information in the cue
file, utilizing ffmpeg.

## Run the application

### Prerequisites

- **ffmpeg** must be in the path of the system.

### Installation

Download the latest release for your system from
the [releases page](https://github.com/RouHim/cue-splatter/releases):

```shell
# Assuming you run a x86/x64 system, if not adjust the binary name to download 
LATEST_VERSION=$(curl -L -s -H 'Accept: application/json' https://github.com/RouHim/cue-splatter/releases/latest | \
sed -e 's/.*"tag_name":"\([^"]*\)".*/\1/') && \
curl -L -o cue-splatter https://github.com/RouHim/cue-splatter/releases/download/$LATEST_VERSION/cue-splatter-x86_64-unknown-linux-musl && \
chmod +x cue-splatter
```

> It's just a static binary, so you can place it anywhere in your system.
> It updates itself, so no need for a package manager.

### Example usage

The following example will split the audio file referenced in the cue file, into multiple audio files based on the track
information, specified in the cue sheet.

```shell
./cue-splatter "path/to/cue/file.cue"
```

You can also point to folders, which are processed recursively.
```shell
./cue-splatter "path/to/some/album a" "another/path/album b"
```

### Container usage

There is also a container image available, that can be used to run the application in a containerized environment.

```shell
docker run --rm -it -v $(pwd):/workdir docker.io/rouhim/cue-splatter:latest /workdir
```

## Development

To build the project, you need to have Rust installed.

### How to build

```shell
cargo build --release
```

### How to run

```shell
cargo run -- "path/to/cue/file.cue"
```
