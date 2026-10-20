// Package fixtures: nested helpers and error guards.
package fixtures

import "errors"

func Validate(value int) (int, error) {
	check := func(v int) bool {
		if v < 0 {
			return false
		}
		return v <= 100
	}
	if !check(value) {
		return 0, errors.New("out of range")
	}
	return value, nil
}

func Count(values []int) int {
	count := 0
	for _, value := range values {
		if value%2 == 0 && value > 0 {
			count++
		}
	}
	return count
}
