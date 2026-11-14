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
