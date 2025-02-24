# reMarkable Screen Capture Utility

A Rust utility for capturing, processing, and extracting handwritten content from reMarkable tablets.

## Overview

This tool connects to a reMarkable tablet over SSH, captures the current screen contents, processes the image to isolate handwritten content, and saves both the full screen and a cropped version containing only the relevant handwritten areas.

## Features

- SSH connection to reMarkable tablet
- Framebuffer extraction from device memory
- Image conversion with proper rotation and contrast
- Automatic detection and cropping of handwritten content
- UI element exclusion to focus only on content

## Requirements

- Rust and Cargo
- FFmpeg for image conversion
- Environment variable `REMARKABLE_IP` set to your tablet's IP address
- SSH access to your reMarkable tablet

## Installation

```bash
git clone https://github.com/yourusername/remarkable-screen-capture.git
cd remarkable-screen-capture
cargo build --release
```

## Usage

1. Set your reMarkable tablet's IP address:
   ```bash
   export REMARKABLE_IP=192.168.1.xxx
   ```

2. Run the utility:
   ```bash
   cargo run --release
   ```

3. The utility will:
   - Connect to your reMarkable
   - Capture the current screen
   - Process the image to enhance readability
   - Detect and isolate handwritten content
   - Create two files:
     - `remarkable_screen.png`: Full screen capture
     - `remarkable_screen_cropped.png`: Cropped version with just the handwritten content

## How It Works

1. Connects to the reMarkable using the OpenSSH crate
2. Locates the `xochitl` process handling the display
3. Finds the framebuffer memory address
4. Extracts raw framebuffer data
5. Converts the raw data to a PNG image using FFmpeg
6. Processes the image to detect contours of handwriting
7. Creates a bounding box around significant content
8. Crops the original image to focus only on the handwritten content

## License

[MIT License](LICENSE)

## Contributing

Contributions welcome! Please feel free to submit a Pull Request.
