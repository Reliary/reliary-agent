// Oracle fixture: JS/TS shapes (B5, B8).
const arrow = (x) => x + 1;
export const arrow2 = (a, b) => {
    return a + b;
};
const not_arrow = 42;
function topfn(x) {
    return x;
}
class Klass {
    method() {
        return 1;
    }
    get prop() {
        return 2;
    }
    constructor() {
        this.x = 1;
    }
}
const result = arrow(1) + topfn(2);
