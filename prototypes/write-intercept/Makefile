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
