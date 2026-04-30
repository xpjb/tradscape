#!/bin/bash

# Edit Caddyfile on VPS via vim's built-in SCP
# After saving, run: ssh root@45.77.218.179 'systemctl reload caddy'

vim scp://root@45.77.218.179//etc/caddy/Caddyfile
