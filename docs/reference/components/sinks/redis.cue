package metadata

components: sinks: redis: {
	title:       "Redis"
	description: "[Redis](\(urls.redis)) is an open source (BSD licensed), in-memory data structure store, used as a database, cache, and message broker."

	classes: {
		commonly_used: false
		delivery:      "best_effort"
		development:   "beta"
		egress_method: "stream"
		service_providers: []
	}

	features: {
		buffer: enabled:      false
		healthcheck: enabled: true
		send: {
			compression: enabled: false
			encoding: {
				enabled: true
				codec: {
					enabled: true
					default: null
					enum: ["json", "text"]
				}
			}
			request: enabled: false
			tls: enabled:     false
			to: {
				service: services.pulsar

				interface: {
					socket: {
						direction: "outgoing"
						protocols: ["tcp"]
						ssl: "disabled"
					}
				}
			}
		}
	}

	support: {
		targets: {
			"aarch64-unknown-linux-gnu":  true
			"aarch64-unknown-linux-musl": true
			"x86_64-apple-darwin":        true
			"x86_64-pc-windows-msv":      true
			"x86_64-unknown-linux-gnu":   true
			"x86_64-unknown-linux-musl":  true
		}

		requirements: []
		warnings: []
		notices: []
	}

	configuration: {
		url: {
			description: "The Redis URL to connect to. The url _must_ take the form of `redis://server:port/db`."
			groups: ["tcp"]
			required: true
			warnings: []
			type: string: {
				examples: ["redis://127.0.0.1:6379/0"]
			}
		}
		key: {
			description: "The Redis key to publish messages to."
			required:    true
			warnings: []
			type: string: {
				examples: ["syslog:{{ app }}", "vector"]
				templateable: true
			}
		}
		data_type: {
			common:      false
			description: "The Redis Data Type(list or channel) to use."
			required:    false
			type: string: {
				default: "list"
				examples: ["list", "channel"]
			}
		}
		method: {
			common:      false
			description: "The Method(lpush or rpush) to publish messages when data_type is list."
			required:    false
			type: string: {
				default: "lpush"
				examples: ["lpush", "rpush"]
			}
		}
	}

	input: {
		logs:    true
		metrics: null
	}
}
