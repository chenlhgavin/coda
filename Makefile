build:
	@cargo build

test:
	@cargo nextest run --all-features

update-submodule:
	@git submodule update --init --recursive --remote

build-release:
	@echo "Building coda-server in release mode..."
	cargo build --release --bin coda-server

deploy: build-release install-service
	@echo "Restarting coda-server..."
	sudo systemctl restart coda-server
	@echo "Deploy complete."
	@systemctl status coda-server --no-pager

install-service:
	@echo "Installing coda-server systemd service..."
	sudo cp deploy/coda-server.service /etc/systemd/system/
	sudo systemctl daemon-reload
	@echo "Done. Run 'sudo systemctl enable --now coda-server' to start."

uninstall-service:
	@echo "Removing coda-server systemd service..."
	-sudo systemctl disable --now coda-server
	sudo rm -f /etc/systemd/system/coda-server.service
	sudo systemctl daemon-reload
	@echo "Done."

restart-service:
	sudo systemctl restart coda-server

status-service:
	@systemctl status coda-server --no-pager

.PHONY: build test update-submodule build-release deploy install-service uninstall-service restart-service status-service
