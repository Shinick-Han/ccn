// Basic branching shapes: the base-1 guard and plain conditionals.
package fixtures;

final class Basics {
    static int identity(int value) {
        return value;
    }

    static int pick(boolean flag) {
        if (flag) {
            return 1;
        }
        return 0;
    }

    static String grade(int score) {
        if (score >= 90) {
            return "A";
        } else if (score >= 80) {
            return "B";
        } else {
            return "C";
        }
    }
}
