CC = gcc
CFLAGS = -O2 -g -Wall -Wextra -Wno-format-truncation -pthread
export TMPDIR = $(CURDIR)/.scratch

all: .scratch/intercept .scratch/probe

.scratch:
	mkdir -p $@

.scratch/intercept: intercept.c | .scratch
	$(CC) $(CFLAGS) $< -o $@

.scratch/probe: probe.c | .scratch
	$(CC) $(CFLAGS) $< -o $@

check: all
	python3 experiment.py coverage

v2: all .scratch/intercept-v2 .scratch/probe-v2

.scratch/intercept-v2: intercept-v2.c intercept.c | .scratch
	$(CC) $(CFLAGS) $< -o $@

.scratch/probe-v2: probe-v2.c probe.c | .scratch
	$(CC) $(CFLAGS) $< -o $@

v3: v2 .scratch/intercept-v3 .scratch/probe-v3

.scratch/intercept-v3: intercept-v3.c intercept.c v3-runtime.h v3-node.h | .scratch
	$(CC) $(CFLAGS) $< -o $@

.scratch/probe-v3: probe-v3.c probe-v2.c probe.c | .scratch
	$(CC) $(CFLAGS) $< -o $@

.scratch/notification-floor-v3: notification-floor-v3.c | .scratch
	$(CC) $(CFLAGS) $< -o $@
