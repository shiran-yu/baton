#!/bin/sh
# UserPromptSubmit: one line of worker quota, so the delegate-or-not choice is made with the numbers in view.
baton="${CLAUDE_PLUGIN_ROOT:-$HOME/.local/share/baton}/bin/baton"
# Cache only: instant, silent when baton or its cache is missing.
line=$("$baton" statusline line 2>/dev/null)
# Refresh a cache older than five minutes for the next prompt (~1.5 s), detached so this prompt never waits.
( command -v setsid >/dev/null && exec setsid "$baton" usage; exec "$baton" usage ) >/dev/null 2>&1 &
[ -n "$line" ] && printf '<baton-quota>%s → read-only research and bounded code: baton:codex first</baton-quota>\n' "$line"
exit 0
