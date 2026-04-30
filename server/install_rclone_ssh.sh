#!/bin/bash
# Interactive apt: -t allocates a TTY so you can confirm with Y/n.
ssh -t root@45.77.218.179 'apt install rclone'
