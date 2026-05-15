#!/usr/bin/env bash
# WaveQL demo: create a sample VCD, run queries, show outputs
set -euo pipefail

WAVEQL="./target/debug/waveql"

if [ ! -x "$WAVEQL" ]; then
    echo "Building waveql..."
    cargo build
fi

VCD_FILE="/tmp/waveql_demo.vcd"

# Create a simple VCD file
cat > "$VCD_FILE" << 'VCD_EOF'
$date
   Today
$end
$version
   waveql demo
$end
$timescale 1ns $end
$scope module top $end
$var wire 1 ! clk $end
$var wire 1 " en $end
$var wire 8 # data $end
$upscope $end
$enddefinitions $end
#0
$dumpvars
0!
0"
b00000000 #
$end
#10
1!
#20
1"
#30
b10100011 #
#40
0!
#50
b01000010 #
#60
1!
#70
0"
#80
0!
#90
b00000000 #
#100
1!
1"
VCD_EOF

echo "=== 1. List signals ==="
$WAVEQL list "$VCD_FILE"

echo ""
echo "=== 2. Changes (JSON) ==="
$WAVEQL changes "$VCD_FILE" --signals top.clk,top.data --from 0ns --to 100ns --format json

echo ""
echo "=== 3. Edges ==="
$WAVEQL edges "$VCD_FILE" --signal top.clk --type rising --from 0ns --to 100ns

echo ""
echo "=== 4. Sample ==="
$WAVEQL sample "$VCD_FILE" --signal top.data --at 37ns

echo ""
echo "=== 5. ASCII view ==="
$WAVEQL ascii "$VCD_FILE" --signals top.clk,top.en,top.data --from 0ns --to 100ns

rm -f "$VCD_FILE"
