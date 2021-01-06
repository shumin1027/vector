package metadata

services: redis: {
	name:     "Redis"
	thing:    "a \(name) database"
	url:      urls.redis
	description: "[Redis](\(urls.redis)) is an open source (BSD licensed), in-memory data structure store, used as a database, cache, and message broker."
	versions: null
}
