.DEFAULT_GOAL := check
CHECK_TASKS := lint test build
TEST_OUTPUTS :=
BUILD_OUTPUTS := target/release/filectrl
FILECTRL_TEST_IMAGE ?= rust:1.98-slim
CONTAINER_BUILD_ARGS := RUST_IMAGE=$(FILECTRL_TEST_IMAGE)
CHECK_NETWORK_LINT := bridge
CHECK_SCRATCH_SIZE := 4g

override COMMAND := $(value COMMAND)
override TEST_ARGS := $(value TEST_ARGS)

export CHECK_TASKS BUILD_OUTPUTS TEST_OUTPUTS CONTAINER_BUILD_ARGS CHECK_NETWORK_LINT CHECK_SCRATCH_SIZE TEST_ARGS COMMAND

export CHECK_TIMEOUT_SECONDS CHECK_MEMORY CHECK_SCRATCH_SIZE CHECK_WORK_SIZE CHECK_TMP_SIZE CHECK_NETWORK_LINT CONTAINER_IMAGE
export CHECK_ADMISSION

.PHONY: check check-info lint test build exec fmt

check lint test build exec:
	@./tests/container/run $@

check-info:
	@./tests/container/run info

fmt:
	@./tests/container/run exec cargo fmt

.PHONY: test-native
test-native:
	@./tests/native/run
