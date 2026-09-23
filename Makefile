# A local TEE ISM devnet.
#
#   make init   build what is missing, start the chain, deploy the enclave, create the ISM
#   make start  run the relayer and the bridge UI
#   make stop   delete the enclave, stop the chain, prune state
#
# Everything runs on this machine except the enclave. A TDX quote has to come from real Intel
# hardware, so that one piece lives on Phala Cloud and is billed by the hour - which is why
# `make stop` deletes it rather than leaving it running.

DEVNET  := devnet
SCRIPTS := $(DEVNET)/scripts

.PHONY: init start stop status logs clean help

help:
	@sed -n 's/^## //p' $(MAKEFILE_LIST)

## init: bring up the chain, the enclave and the ISM
init:
	@$(SCRIPTS)/10-celestia-up.sh
	@$(SCRIPTS)/20-celestia-hyperlane.sh
	@$(SCRIPTS)/30-enclave-up.sh
	@$(SCRIPTS)/50-warp-celestia.sh
	@$(SCRIPTS)/80-evm-isms.sh
	@$(SCRIPTS)/85-celestia-isms.sh
	@$(SCRIPTS)/90-evm-warp.sh
	@echo
	@echo "devnet is ready. 'make start' runs the relayer and the UI."

## start: run the relayer and the bridge UI on localhost:3000
start:
	@$(SCRIPTS)/60-start.sh

## stop: delete the enclave, stop the chain, prune state
stop:
	@$(SCRIPTS)/stop.sh

## status: show what is deployed and where
status:
	@$(SCRIPTS)/status.sh

## logs: follow the chain's logs
logs:
	@docker logs -f teeism-celestia

## clean: stop, and also remove the built binaries and images
clean:
	@KEEP_BIN=0 $(SCRIPTS)/stop.sh
	@docker image rm celestia-app-teeism:local 2>/dev/null || true
