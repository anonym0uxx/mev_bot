#!/usr/bin/env bash
# Copies service file and enables it
sudo cp scripts/pump-quant-rust.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable pump-quant-rust
echo "Installed. Start with: sudo systemctl start pump-quant-rust"
