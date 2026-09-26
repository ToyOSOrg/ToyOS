slint::slint! {
    import { Button, VerticalBox } from "std-widgets.slint";
    export component App inherits Window {
        in-out property <int> count: 0;
        VerticalBox {
            Text { text: "Hello from Slint, clicked " + count + " times"; }
            Button { text: "Click me"; clicked => { count += 1; } }
        }
    }
}

fn main() {
    App::new().unwrap().run().unwrap();
}
