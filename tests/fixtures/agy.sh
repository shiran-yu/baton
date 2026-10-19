#!/bin/sh
set -eu
schema=
previous=
for arg in "$@"; do
    if [ "$previous" = '--json-schema' ]; then schema=$arg; fi
    previous=$arg
done
taskdir=${schema%/*}
printf '%s\n' "$@" > "$taskdir/agy-argv.txt"
env > "$taskdir/agy-environment.txt"
printf '%s' "$$" > "$taskdir/agy.pid"
printf '%s\n' '{"event":"init","conversation_id":"fake-agy","init":{"cwd":".","tools":["write_to_file","finish"],"permission_mode":"always-proceed"}}'
turn=0
mode=
while IFS= read -r line; do
    turn=$((turn + 1))
    printf '%s\n' "$line" >> "$taskdir/agy-turns.jsonl"
    if [ "$turn" = 1 ]; then
        mode=$line
        case "$mode" in *FAKE_NO_EDIT*) ;; *) mkdir -p src; printf '%s\n' hello > src/output.txt ;; esac
    else
        printf '%s\n' fixed >> src/output.txt
    fi
    step=$((turn * 2))
    printf '%s\n' "{\"event\":\"step_update\",\"step_update\":{\"conversation_id\":\"fake-agy\",\"step_index\":$step,\"state\":\"ACTIVE\",\"step_type\":\"tool\",\"tool_name\":\"write_to_file\"}}"
    printf '%s\n' "{\"event\":\"step_update\",\"step_update\":{\"conversation_id\":\"fake-agy\",\"step_index\":$step,\"state\":\"DONE\",\"step_type\":\"tool\",\"tool_info\":{\"name\":\"write_to_file\"}}}"
    printf '%s\n' "{\"event\":\"step_update\",\"step_update\":{\"conversation_id\":\"fake-agy\",\"step_index\":$((step + 1)),\"state\":\"DONE\",\"step_type\":\"agent_response\",\"usage\":{\"input_tokens\":100,\"cache_read_tokens\":40,\"output_tokens\":10,\"thinking_tokens\":3}}}"
    status=SUCCESS
    case "$mode" in *FAKE_RESULT_ERROR*) status=ERROR ;; esac
    denied='[]'
    structured=',"structured_output":{"status":"done","files_changed":[],"acceptance":[],"notes":"Finished.","questions":[]}'
    case "$mode" in
        *FAKE_DENY*) denied='[{"tool_name":"run_command","reason":"permission check failed"}]' ;;
        *FAKE_STEP_DENY*) printf '%s\n' '{"event":"step_update","step_update":{"conversation_id":"fake-agy","step_index":99,"state":"ERROR","step_type":"tool","tool_info":{"name":"run_command","error":{"message":"permission check failed: run_command"}}}}' ;;
        *FAKE_STDERR_DENY*) printf '%s\n' 'jetski: no output produced' >&2 ;;
        *FAKE_NO_STRUCTURED*) structured= ;;
        *FAKE_TIMEOUT*) trap '' TERM; (sleep 8; printf survived > "$taskdir/child-survived"; sleep 28) & wait ;;
    esac
    printf '%s\n' "{\"event\":\"result\",\"result\":{\"conversation_id\":\"fake-agy\",\"status\":\"$status\",\"response\":\"Finished.\",\"usage\":{\"input_tokens\":$((turn * 100)),\"cache_read_tokens\":$((turn * 40)),\"output_tokens\":$((turn * 10)),\"thinking_tokens\":$((turn * 3))},\"denied_actions\":$denied$structured}}"
done
case "$mode" in *FAKE_LATE_STDERR_DENY*) printf '%s\n' 'jetski: no output produced' >&2 ;; esac
printf '%s\n' closed > "$taskdir/agy-closed"
