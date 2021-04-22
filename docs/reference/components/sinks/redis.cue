package metadata

components: sinks: redis: {
	title: "Redis"
	classes: {
		commonly_used: false
		delivery:      "best_effort"
		development:   "beta"
		egress_method: "stream"
		service_providers: []
		stateful: false
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
					default: "json"
					enum: ["json", "text"]
				}
			}
			request: enabled: false
			tls: {
				enabled:                true
				can_enable:             true
				can_verify_certificate: false
				can_verify_hostname:    false
				enabled_default:        false
			}
			to: {
				service: services.redis
				interface: {
					socket: {
						direction: "outgoing"
						protocols: ["tcp"]
						ssl: "optional"
					}
				}
			}
		}
	}

	support: {
		targets: {
			"aarch64-unknown-linux-gnu":      true
			"aarch64-unknown-linux-musl":     true
			"armv7-unknown-linux-gnueabihf":  true
			"armv7-unknown-linux-musleabihf": true
			"x86_64-apple-darwin":            true
			"x86_64-pc-windows-msv":          true
			"x86_64-unknown-linux-gnu":       true
			"x86_64-unknown-linux-musl":      true
		}

		requirements: []
		warnings: []
		notices: []
	}

	configuration: {
		url: {
			description: "The Redis URL to connect to. The url _must_ take the form of `protocol://server:port/db` where the protocol can either be `redis` or `rediss` for connections secured via TLS."
			groups: ["tcp"]
			required: true
			warnings: []
			type: string: {
				examples: ["redis://127.0.0.1:6379/0"]
				syntax: "literal"
			}
		}
		key: {
			description: "The Redis key to publish messages to."
			required:    true
			warnings: []
			type: string: {
				examples: ["syslog:{{ app }}", "vector"]
				syntax: "template"
			}
		}
		data_type: {
			common:      false
			description: "The Redis data type (`list` or `channel`) to use."
			required:    false
			type: string: {
				default: "list"
				enum: {
					list:    "Use the Redis `list` data type."
					channel: "Use the Redis `channel` data type."
				}
				syntax: "literal"
			}
		}
		method: {
			common:      false
			description: "The method (`lpush` or `rpush`) to publish messages when `data_type` is list."
			required:    false
			type: string: {
				default: "lpush"
				enum: {
					lpush: "Use the `lpush` method to publish messages."
					rpush: "Use the `rpush` method to publish messages."
				}
				syntax: "literal"
			}
		}
	}

	input: {
		logs:    true
		metrics: null
	}

	how_it_works: {
		redis_rs: {
			title: "redis-rs"
			body:  """
				The `redis` sink uses [`redis-rs`](\(urls.redis_rs)) under the hood, which is a high level Redis library
				for Rust. It provides convenient access to all Redis functionality through a very flexible but low-level
				API.
				"""
		}
	}
}
