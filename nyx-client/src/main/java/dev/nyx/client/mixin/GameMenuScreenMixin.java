package dev.nyx.client.mixin;

import dev.nyx.client.screen.NyxSettingsScreen;
import net.minecraft.client.MinecraftClient;
import net.minecraft.client.gui.Element;
import net.minecraft.client.gui.Drawable;
import net.minecraft.client.gui.Selectable;
import net.minecraft.client.gui.screen.GameMenuScreen;
import net.minecraft.client.gui.widget.ButtonWidget;
import net.minecraft.text.Text;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Shadow;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(GameMenuScreen.class)
public abstract class GameMenuScreenMixin {
    @Shadow
    protected abstract <T extends Element & Drawable & Selectable> T addDrawableChild(T drawableElement);

    @Inject(method = "init", at = @At("TAIL"))
    private void nyx$addClientButton(CallbackInfo ci) {
        int width = MinecraftClient.getInstance().getWindow().getScaledWidth();
        addDrawableChild(ButtonWidget.builder(Text.literal("NYX CLIENT"), button ->
            MinecraftClient.getInstance().setScreen(new NyxSettingsScreen((GameMenuScreen) (Object) this))
        ).dimensions(width - 118, 10, 108, 22).build());
    }
}
