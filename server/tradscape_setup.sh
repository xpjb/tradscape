#!/bin/bash

# One-time bootstrap: creates only tradscape.service under /root/tradscape (binary listens on 8081).
# Does not modify iannet or other units. `daemon-reload` reloads unit definitions only; it does not restart unrelated running services.
TARGET_HOST="45.77.218.179"
TARGET_USER="root"
SERVICE_NAME="tradscape.service"
BINARY_PATH="/root/tradscape/tradscape-server"

echo "Deploying $SERVICE_NAME to $TARGET_USER@$TARGET_HOST..."

ssh "$TARGET_USER@$TARGET_HOST" "bash -s" <<EOF

    echo "Creating /root/tradscape directory..."
    mkdir -p /root/tradscape

    echo "Creating service file at /etc/systemd/system/$SERVICE_NAME..."
    cat > /etc/systemd/system/$SERVICE_NAME <<SERVICE_DEF
[Unit]
Description=Tradscape site server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=$BINARY_PATH
Restart=always
User=root
WorkingDirectory=$(dirname $BINARY_PATH)

[Install]
WantedBy=multi-user.target
SERVICE_DEF

    echo "Reloading systemd daemon..."
    systemctl daemon-reload

    echo "Enabling $SERVICE_NAME..."
    systemctl enable $SERVICE_NAME

    echo "Starting $SERVICE_NAME..."
    systemctl start $SERVICE_NAME

    echo "Current Status:"
    systemctl status $SERVICE_NAME --no-pager

EOF

echo "Deployment complete."
