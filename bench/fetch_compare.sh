#!/usr/bin/env bash
# Bench A — ar5iv vs e-print cold-fetch latency.
# Reads arXiv IDs from test-papers.txt; emits TSV of per-trial timings and a summary.
#
# Usage: bench/fetch_compare.sh [path/to/test-papers.txt] [trials]

set -euo pipefail

PAPERS_FILE="${1:-test-papers.txt}"
TRIALS="${2:-3}"
MAX_SECS=60

IDS=$(grep -Eo '^[[:space:]]*[0-9]{4}\.[0-9]{4,5}([[:space:]]|$)' "$PAPERS_FILE" \
       | tr -d ' ' | sort -u)

if [[ -z "$IDS" ]]; then
  echo "no arXiv IDs parsed from $PAPERS_FILE" >&2
  exit 1
fi

echo "# Bench A: ar5iv vs e-print"
echo "# papers_file=$PAPERS_FILE trials=$TRIALS max_secs=$MAX_SECS"
echo "# columns: id kind trial time_total_s bytes status"

raw=$(mktemp)
for id in $IDS; do
  for kind in ar5iv eprint; do
    case "$kind" in
      ar5iv)  url="https://ar5iv.labs.arxiv.org/html/$id" ;;
      eprint) url="https://arxiv.org/e-print/$id" ;;
    esac
    for t in $(seq 1 "$TRIALS"); do
      out=$(curl -sSL --max-time "$MAX_SECS" -o /dev/null \
              -w "%{time_total}\t%{size_download}\t%{http_code}" \
              "$url" 2>/dev/null || echo -e "ERR\tERR\tERR")
      line="$id	$kind	$t	$out"
      echo "$line"
      echo "$line" >> "$raw"
    done
  done
done

echo
echo "# summary (min / median / max seconds, mean bytes, status mode)"
echo "# id kind min med max mean_bytes mode_status"

# Group by (id, kind) and emit sorted-by-time rows so awk can pick min/median/max.
awk -F'\t' '$5 != "ERR" { print $1"\t"$2"\t"$4"\t"$5"\t"$6 }' "$raw" \
  | sort -t'	' -k1,1 -k2,2 -k3,3n \
  | awk -F'\t' '
    function flush(  med) {
      if (n == 0) return
      if (n % 2) med = t[int(n/2)+1]
      else       med = (t[n/2] + t[n/2+1]) / 2
      best=""; bestn=0
      for (s in scount) if (scount[s] > bestn) { best=s; bestn=scount[s] }
      printf "%s\t%s\t%.3f\t%.3f\t%.3f\t%d\t%s\n", cur_id, cur_kind, t[1], med, t[n], bsum/n, best
      n=0; bsum=0; delete t; delete scount
    }
    {
      if ($1 != cur_id || $2 != cur_kind) { flush(); cur_id=$1; cur_kind=$2 }
      n++; t[n]=$3; bsum+=$4; scount[$5]++
    }
    END { flush() }
  '

rm -f "$raw"
