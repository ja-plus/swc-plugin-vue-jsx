import { defineComponent } from 'vue';
type T = {
    bar: number;
};
type S = {
    nested: {
        foo: T['bar'];
    };
};
defineComponent((props: S['nested'])=>{}, {
    props: {
        foo: {
            type: Number,
            required: true
        }
    }
});
