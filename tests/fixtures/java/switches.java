// A three-case switch (no default, so case and section counts agree),
// try/catch, and the conditional operator.
package fixtures;

final class Switches {
    static String route(String method) {
        switch (method) {
            case "GET":
                return "read";
            case "POST":
                return "write";
            case "DELETE":
                return "purge";
        }
        return "unknown";
    }

    static int safeDivide(int a, int b) {
        try {
            return a / b;
        } catch (ArithmeticException e) {
            return 0;
        }
    }

    static int clamp(int value) {
        return value < 0 ? 0 : value;
    }
}
