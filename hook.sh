#!/bin/bash
[ "${AGENT_HOOKS:-1}" = "0" ] && exit 0

launcher="stanza hook"
source "$(dirname "$0")/launch.sh"

"${stanza[@]}" hook ${STANZA_FLAGS-}
status=$?
[ "$status" -eq 2 ] && exit 1
exit "$status"
