#!/usr/bin/env bash
# Creates the assay plan labels and applies them. Needs gh authenticated with write access.
set -euo pipefail
R=Wet-Ink-Corporation/assay
gh label create "advisory-first" -R "$R" -c c5def5 --force
gh label create "area:app" -R "$R" -c 1d76db --force
gh label create "area:cli" -R "$R" -c 1d76db --force
gh label create "area:docs" -R "$R" -c 1d76db --force
gh label create "area:domain" -R "$R" -c 1d76db --force
gh label create "area:eval" -R "$R" -c 1d76db --force
gh label create "area:forge" -R "$R" -c 1d76db --force
gh label create "area:gates" -R "$R" -c 1d76db --force
gh label create "area:ops" -R "$R" -c 1d76db --force
gh label create "area:plugin" -R "$R" -c 1d76db --force
gh label create "area:runner" -R "$R" -c 1d76db --force
gh label create "area:workflows" -R "$R" -c 1d76db --force
gh label create "calibration" -R "$R" -c 0e8a16 --force
gh label create "canary" -R "$R" -c fbca04 --force
gh label create "owner-action" -R "$R" -c d93f0b --force
gh label create "security-control" -R "$R" -c b60205 --force
gh label create "upstream" -R "$R" -c 5319e7 --force
gh issue edit 35 -R "$R" --add-label "area:docs"
gh issue edit 36 -R "$R" --add-label "owner-action" --add-label "area:docs"
gh issue edit 37 -R "$R" --add-label "owner-action" --add-label "calibration"
gh issue edit 38 -R "$R" --add-label "area:docs" --add-label "security-control"
gh issue edit 39 -R "$R" --add-label "owner-action" --add-label "security-control"
gh issue edit 40 -R "$R" --add-label "owner-action"
gh issue edit 41 -R "$R" --add-label "owner-action" --add-label "security-control"
gh issue edit 42 -R "$R" --add-label "owner-action" --add-label "security-control"
gh issue edit 43 -R "$R" --add-label "owner-action" --add-label "security-control"
gh issue edit 44 -R "$R" --add-label "upstream"
gh issue edit 45 -R "$R" --add-label "upstream"
gh issue edit 46 -R "$R" --add-label "area:cli"
gh issue edit 47 -R "$R" --add-label "area:domain" --add-label "security-control"
gh issue edit 48 -R "$R" --add-label "area:domain"
gh issue edit 49 -R "$R" --add-label "area:domain" --add-label "security-control"
gh issue edit 50 -R "$R" --add-label "area:domain" --add-label "security-control"
gh issue edit 51 -R "$R" --add-label "area:domain"
gh issue edit 52 -R "$R" --add-label "area:forge" --add-label "security-control"
gh issue edit 53 -R "$R" --add-label "area:forge" --add-label "security-control"
gh issue edit 54 -R "$R" --add-label "area:app"
gh issue edit 55 -R "$R" --add-label "area:app"
gh issue edit 56 -R "$R" --add-label "area:app" --add-label "security-control"
gh issue edit 57 -R "$R" --add-label "area:app"
gh issue edit 58 -R "$R" --add-label "area:app" --add-label "security-control"
gh issue edit 59 -R "$R" --add-label "area:app"
gh issue edit 60 -R "$R" --add-label "area:workflows" --add-label "security-control"
gh issue edit 61 -R "$R" --add-label "area:workflows" --add-label "security-control"
gh issue edit 62 -R "$R" --add-label "area:gates"
gh issue edit 63 -R "$R" --add-label "area:gates" --add-label "security-control"
gh issue edit 64 -R "$R" --add-label "area:gates" --add-label "security-control"
gh issue edit 65 -R "$R" --add-label "area:gates" --add-label "security-control"
gh issue edit 66 -R "$R" --add-label "area:gates" --add-label "security-control"
gh issue edit 67 -R "$R" --add-label "area:gates" --add-label "area:workflows"
gh issue edit 68 -R "$R" --add-label "area:workflows" --add-label "security-control"
gh issue edit 69 -R "$R" --add-label "area:workflows" --add-label "security-control"
gh issue edit 70 -R "$R" --add-label "area:forge"
gh issue edit 71 -R "$R" --add-label "area:forge"
gh issue edit 72 -R "$R" --add-label "area:app"
gh issue edit 73 -R "$R" --add-label "area:workflows" --add-label "area:app"
gh issue edit 74 -R "$R" --add-label "area:cli"
gh issue edit 75 -R "$R" --add-label "area:cli" --add-label "security-control"
gh issue edit 76 -R "$R" --add-label "area:gates" --add-label "security-control"
gh issue edit 77 -R "$R" --add-label "area:docs"
gh issue edit 78 -R "$R" --add-label "canary" --add-label "security-control"
gh issue edit 79 -R "$R" --add-label "calibration" --add-label "security-control"
gh issue edit 80 -R "$R" --add-label "calibration"
gh issue edit 81 -R "$R" --add-label "owner-action"
gh issue edit 82 -R "$R" --add-label "area:runner"
gh issue edit 83 -R "$R" --add-label "area:runner" --add-label "security-control"
gh issue edit 84 -R "$R" --add-label "area:runner" --add-label "security-control"
gh issue edit 85 -R "$R" --add-label "area:workflows" --add-label "security-control"
gh issue edit 86 -R "$R" --add-label "area:domain" --add-label "security-control"
gh issue edit 87 -R "$R" --add-label "area:runner" --add-label "security-control"
gh issue edit 88 -R "$R" --add-label "area:app" --add-label "security-control"
gh issue edit 89 -R "$R" --add-label "calibration" --add-label "security-control" --add-label "canary"
gh issue edit 90 -R "$R" --add-label "area:plugin"
gh issue edit 91 -R "$R" --add-label "area:app"
gh issue edit 92 -R "$R" --add-label "area:domain"
gh issue edit 93 -R "$R" --add-label "area:docs"
gh issue edit 94 -R "$R" --add-label "calibration"
gh issue edit 95 -R "$R" --add-label "area:plugin" --add-label "area:app"
gh issue edit 96 -R "$R" --add-label "area:app"
gh issue edit 97 -R "$R" --add-label "area:app"
gh issue edit 98 -R "$R" --add-label "area:app"
gh issue edit 99 -R "$R" --add-label "area:app"
gh issue edit 100 -R "$R" --add-label "area:app"
gh issue edit 101 -R "$R" --add-label "area:app"
gh issue edit 103 -R "$R" --add-label "area:workflows" --add-label "area:app"
gh issue edit 104 -R "$R" --add-label "calibration"
gh issue edit 105 -R "$R" --add-label "calibration"
gh issue edit 106 -R "$R" --add-label "calibration" --add-label "upstream"
gh issue edit 107 -R "$R" --add-label "area:cli"
gh issue edit 108 -R "$R" --add-label "area:plugin"
gh issue edit 109 -R "$R" --add-label "area:gates" --add-label "advisory-first"
gh issue edit 110 -R "$R" --add-label "area:gates" --add-label "upstream" --add-label "security-control"
gh issue edit 111 -R "$R" --add-label "area:eval"
gh issue edit 112 -R "$R" --add-label "area:eval"
gh issue edit 113 -R "$R" --add-label "area:ops" --add-label "area:cli"
gh issue edit 114 -R "$R" --add-label "area:workflows"
gh issue edit 115 -R "$R" --add-label "calibration"
gh issue edit 116 -R "$R" --add-label "owner-action"
gh issue edit 117 -R "$R" --add-label "owner-action" --add-label "advisory-first"
gh issue edit 118 -R "$R" --add-label "owner-action"
