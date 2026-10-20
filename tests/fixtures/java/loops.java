// Classic `for`, `while` and enhanced `for`.
package fixtures;

final class Loops {
    static int sum(int[] values) {
        int total = 0;
        for (int i = 0; i < values.length; i++) {
            total += values[i];
        }
        return total;
    }

    static int countdown(int start) {
        int steps = 0;
        while (start > 0) {
            start--;
            steps++;
        }
        return steps;
    }

    static int total(int[] values) {
        int result = 0;
        for (int value : values) {
            result += value;
        }
        return result;
    }
}
