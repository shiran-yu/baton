#!/bin/sh
set -eu
resume=no
if [ "${2-}" = resume ]; then resume=yes; fi
output=
previous=
read_only=no
for arg in "$@"; do
    if [ "$previous" = '-o' ]; then output=$arg; fi
    if [ "$previous" = '-s' ] && [ "$arg" = read-only ]; then read_only=yes; fi
    previous=$arg
done
taskdir=${output%/*}
printf '%s' "$$" > "$taskdir/worker.pid"
printf '%s\n' "$@" > "$taskdir/argv-$resume.txt"
env > "$taskdir/environment.txt"
input=$(cat)
printf '%s' "$input" > "$taskdir/input-$resume.txt"
case "$input" in
    *FAKE_WAIT_SLEEP*) sleep 3 ;;
    *FAKE_SLEEP*)
        (sleep 2; printf survived > "$taskdir/child-survived"; sleep 28) &
        printf '%s' "$!" > "$taskdir/child.pid"
        wait
        ;;
    *FAKE_AUTH*) printf '%s\n' 'authentication required' >&2; exit 1 ;;
esac
printf '%s\n' '{"type":"thread.started","thread_id":"fake-thread-123"}' '{"type":"turn.started"}'
case "$input" in
    *FAKE_QUOTA*) printf '%s\n' '{"type":"turn.failed","error":{"message":"You have reached your usage limit.\nTry later."}}'; exit 1 ;;
    *FAKE_RATE_LIMIT*) printf '%s\n' '{"type":"error","message":"rate limit reached"}'; exit 0 ;;
    *FAKE_LIVE*)
        day=$(date -u -d yesterday +%Y/%m/%d 2>/dev/null || date -u -v-1d +%Y/%m/%d)
        mkdir -p "$CODEX_HOME/sessions/$day"
        rollout="$CODEX_HOME/sessions/$day/rollout-test-fake-thread-123.jsonl"
        printf '%s' "$rollout" > "$taskdir/rollout-path"
        printf '%s\n' '{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":12000,"cached_input_tokens":8000,"output_tokens":150}},"rate_limits":{"primary":{"used_percent":34,"window_minutes":300,"resets_at":9999999999},"secondary":{"used_percent":7,"window_minutes":10080,"resets_at":9999999999}}}}' > "$rollout"
        tries=0
        while [ ! -f "$taskdir/release-worker" ] && [ "$tries" -lt 150 ]; do
            sleep 0.1
            tries=$((tries + 1))
        done
        ;;
esac
printf '%s\n' '{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"/bin/zsh -lc \"cargo test\"","aggregated_output":"","exit_code":null,"status":"in_progress"}}'
original=$(cat "$taskdir/input-no.txt")
case "$read_only:$original" in
    yes:*) ;;
    *FAKE_NO_EDIT*) ;;
    *)
        mkdir -p src
        if [ "$resume" = yes ]; then printf '%s\n' fixed >> src/output.txt
        else printf '%s\n' hello > src/output.txt; fi
        ;;
esac
case "$input" in *FAKE_OUT_OF_SCOPE*) printf '%s\n' outside > outside.txt ;; esac
printf '%s\n' '{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"/bin/zsh -lc \"cargo test\"","aggregated_output":"ok","exit_code":0,"status":"completed"}}'
printf '%s\n' '{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"Finished the requested work."}}'
printf '%s\n' '{"type":"turn.completed","usage":{"input_tokens":190369,"cached_input_tokens":156288,"cache_write_input_tokens":0,"output_tokens":5927,"reasoning_output_tokens":512}}'
case "$input" in
    *FAKE_FAIL_EVENT*) printf '%s\n' '{"type":"turn.failed","error":{"message":"worker turn failed"}}' ;;
esac
case "$input" in
    *FAKE_INVALID*) printf '%s' 'this is not JSON' > "$output" ;;
    *FAKE_DECISION*) printf '%s' '{"status":"needs_decision","files_changed":[],"acceptance":[],"notes":"Choose one","questions":["Which approach?"]}' > "$output" ;;
    *) printf '%s' '{"status":"done","files_changed":["invented.txt"],"acceptance":[],"notes":"Finished.","questions":[]}' > "$output" ;;
esac
