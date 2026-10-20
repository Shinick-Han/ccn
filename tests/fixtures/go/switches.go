// Package fixtures: switch statements and loops.
package fixtures

func Route(kind string) string {
	switch kind {
	case "GET":
		return "read"
	case "POST":
		return "write"
	default:
		return "unknown"
	}
}

func Kind(value interface{}) string {
	switch value.(type) {
	case int:
		return "int"
	case string:
		return "string"
	default:
		return "other"
	}
}

func Sum(values []int) int {
	total := 0
	for _, value := range values {
		total += value
	}
	return total
}
