#!/usr/bin/env bash
# Bench B — ar5iv coverage across arXiv eras and categories.
# Samples IDs from arXiv's Atom API, then HEAD-checks each on ar5iv.
#
# Usage: bench/ar5iv_coverage.sh [per_bucket]

set -euo pipefail

PER_BUCKET="${1:-25}"
ARXIV_API="http://export.arxiv.org/api/query"
AR5IV_BASE="https://ar5iv.labs.arxiv.org/html"

sample_ids() {
  local query="$1" sort_order="$2" max="$3"
  curl -sSL --max-time 30 -G "$ARXIV_API" \
      --data-urlencode "search_query=$query" \
      --data-urlencode "sortBy=submittedDate" \
      --data-urlencode "sortOrder=$sort_order" \
      --data-urlencode "max_results=$max" \
    | grep -Eo 'https?://arxiv.org/abs/[^<"]+' \
    | sed -E 's|https?://arxiv.org/abs/||; s|v[0-9]+$||' \
    | sort -u | head -n "$max"
}

check_ar5iv() {
  local id="$1"
  curl -sS -o /dev/null -L --max-time 15 -w "%{http_code}" "$AR5IV_BASE/$id" 2>/dev/null \
    || echo "ERR"
}

echo "# Bench B: ar5iv coverage"
echo "# per_bucket=$PER_BUCKET"
echo "# columns: bucket id status"

raw=$(mktemp)

declare -a BUCKETS=(
  "recent_cs:cat:cs.LG:descending"
  "recent_math:cat:math.AG:descending"
  "old_cs:cat:cs.CL:ascending"
  "old_math:cat:math.GT:ascending"
)

for spec in "${BUCKETS[@]}"; do
  IFS=: read -r name qkey qval order <<< "$spec"
  query="$qkey:$qval"
  ids=$(sample_ids "$query" "$order" "$PER_BUCKET" || true)
  for id in $ids; do
    s=$(check_ar5iv "$id")
    line="$name	$id	$s"
    echo "$line"
    echo "$line" >> "$raw"
  done
done

echo
echo "# summary: success rate per bucket (HTTP 200)"
echo "# bucket total ok_200 redirect_3xx error_4xx_5xx pct_200"
awk -F'\t' '
  { tot[$1]++; if ($3 ~ /^2/) ok[$1]++; else if ($3 ~ /^3/) rd[$1]++; else err[$1]++ }
  END {
    for (b in tot) {
      printf "%s\t%d\t%d\t%d\t%d\t%.1f\n", b, tot[b], ok[b]+0, rd[b]+0, err[b]+0, (ok[b]+0)*100.0/tot[b]
    }
  }
' "$raw" | sort

rm -f "$raw"
