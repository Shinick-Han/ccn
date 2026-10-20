// Lambdas are their own scope: an `if` inside one belongs to the lambda,
// never to the method that creates it.
package fixtures;

import java.util.function.IntUnaryOperator;

final class Lambdas {
    static IntUnaryOperator absolute() {
        return value -> {
            if (value < 0) {
                return -value;
            }
            return value;
        };
    }

    static int countAbove(int[] values, int limit) {
        IntUnaryOperator bump = value -> value + 1;
        int count = 0;
        for (int i = 0; i < values.length; i++) {
            if (values[i] > limit) {
                count += bump.applyAsInt(0);
            }
        }
        return count;
    }
}
