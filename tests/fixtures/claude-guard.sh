#!/bin/sh
input=$(/bin/cat)
if [ -n "$BATON_HOME" ]; then
    printf '%s' "$input" > "$BATON_HOME/guard-input.json"
fi
case "$input" in
    *'"command":"rm -rf '*)
        printf '%s\n' '{"hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"recursive deletion blocked"}}'
        ;;
    *)
        printf '%s\n' '{"hookSpecificOutput":{"permissionDecision":"allow"}}'
        ;;
esac
