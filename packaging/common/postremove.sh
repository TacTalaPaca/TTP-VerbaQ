#!/bin/sh
if command -v udevadm >/dev/null 2>&1; then
    udevadm control --reload-rules || true
    udevadm trigger --subsystem-match=input --action=change || true
    udevadm trigger --subsystem-match=misc --action=change || true
fi
exit 0
