#!/bin/sh
# Packages needed to *build* recall on Ubuntu 22.04+ (plus Rust from rustup.rs).
# The Vosk library and the Vulkan SDK's shader compiler (for whisper.cpp's
# GPU backend) are not packaged by Ubuntu 22.04: the fetch scripts download
# them into vendor/.
set -e
sudo apt-get install -y build-essential pkg-config curl python3 cmake libclang-dev xz-utils \
  libgtk-4-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev liborc-0.4-dev \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-pulseaudio \
  pulseaudio-utils
"$(dirname "$0")/fetch-vosk.sh"
"$(dirname "$0")/fetch-vulkan-sdk.sh"
