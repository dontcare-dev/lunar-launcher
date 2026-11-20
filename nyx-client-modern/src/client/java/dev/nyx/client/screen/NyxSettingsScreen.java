package dev.nyx.client.screen;

import net.minecraft.client.Minecraft;
import net.minecraft.client.Options;
import net.minecraft.client.gui.GuiGraphicsExtractor;
import net.minecraft.client.gui.components.Button;
import net.minecraft.client.gui.screens.Screen;
import net.minecraft.network.chat.Component;

public final class NyxSettingsScreen extends Screen {
    private final Screen parent;

    public NyxSettingsScreen(Screen parent) {
        super(Component.literal("Nyx Client"));
        this.parent = parent;
    }

    @Override
    protected void init() {
        Minecraft client = Minecraft.getInstance();
        Options options = client.options;
        int x = width / 2 - 150;
        int w = 300;
        addRenderableWidget(options.renderDistance().createButton(options, x, 82, w));
        addRenderableWidget(options.simulationDistance().createButton(options, x, 116, w));
        addRenderableWidget(options.framerateLimit().createButton(options, x, 150, w));
        addRenderableWidget(options.enableVsync().createButton(options, x, 184, w));
        addRenderableWidget(Button.builder(Component.literal("APPLY PERFORMANCE PRESET"), button -> {
            options.renderDistance().set(6);
            options.simulationDistance().set(4);
            options.framerateLimit().set(260);
            options.enableVsync().set(false);
            options.save();
            clearWidgets();
            init();
        }).bounds(x, 232, w, 24).build());
        addRenderableWidget(Button.builder(Component.literal("BACK"), button -> onClose())
            .bounds(x, 266, w, 24).build());
    }

    @Override
    public void extractRenderState(GuiGraphicsExtractor graphics, int mouseX, int mouseY, float delta) {
        super.extractRenderState(graphics, mouseX, mouseY, delta);
        graphics.fill(0, 0, width, height, 0xF5070B16);
        graphics.fill(width / 2 - 170, 18, width / 2 + 170, 62, 0xAA171F35);
        graphics.text(font, "NYX CLIENT", width / 2 - 42, 28, 0xFFB9C8FF, true);
        graphics.text(font, "26.1.2 / PERFORMANCE CONTROL", width / 2 - 94, 44, 0xFF8999C0, false);
    }

    @Override
    public void onClose() {
        Minecraft.getInstance().setScreen(parent);
    }
}
