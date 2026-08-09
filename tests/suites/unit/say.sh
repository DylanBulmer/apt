#!/usr/bin/env bash
# mc_say_command: broadcasts must carry no "[Rcon]" prefix and be valid JSON.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../../lib/assert.sh"
source "$MC_COMMON"

# What the server would actually display, recovered from the emitted command.
shown() { mc_say_command "$@" | sed 's/^tellraw @a //' | jq -r '.text'; }
valid() { mc_say_command "$@" | sed 's/^tellraw @a //' | jq -e . >/dev/null 2>&1 && echo yes || echo no; }

section "the real countdown messages"
check "command form" 'tellraw @a {"text":"[Server] Shutting down in 5 minutes."}' \
      "$(mc_say_command "[Server] Shutting down in 5 minutes.")"
check "no [Rcon] anywhere" "" "$(mc_say_command "[Server] Shutting down in 1 minute." | grep -o Rcon || true)"
check "not a say command"  "" "$(mc_say_command "[Server] x" | grep -o '^say' || true)"

section "round-trips through the server's JSON parser"
for m in "[Server] Shutting down in 5 minutes." \
         "[Server] Shutting down in 30 seconds." \
         "[mc] Backup starting — brief lag possible" \
         "[mc] Backup complete"; do
    check "valid JSON: ${m:0:26}" yes  "$(valid "$m")"
    check "text preserved"        "$m" "$(shown "$m")"
done

section "escaping (literals today, but this is a command sink)"
check "double quote"   'he said "hi"' "$(shown 'he said "hi"')"
check "backslash"      'a\b'          "$(shown 'a\b')"
check "both"           'a\"b'         "$(shown 'a\"b')"
check "valid with quotes"    yes "$(valid 'he said "hi"')"
check "valid with backslash" yes "$(valid 'a\b')"
check "injection is inert"   '"} ] junk [ {"' "$(shown '"} ] junk [ {"')"
check "  ... still valid"    yes "$(valid '"} ] junk [ {"')"
check "newline flattened"    "a b" "$(shown "$(printf 'a\nb')")"
check "CR flattened"         "a b" "$(shown "$(printf 'a\rb')")"

section "unicode (the backup message carries an em dash)"
check "em dash survives" "— ok" "$(shown "— ok")"

report
