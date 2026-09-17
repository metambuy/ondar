// The popover's root view. Holds the only state shared across panes — at M2c that is nothing
// yet — and renders the dev transport.
import styles from "./panel.module.css";
import Transport from "./Transport";

export default function Panel() {
  return (
    <main className={styles.panel}>
      <Transport />
    </main>
  );
}
